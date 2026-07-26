# Issue #28 — Cell capability admission, effect ledger, audit/provenance design

Status: accepted for implementation on branch `worktree-issue-28-cell-capability`.
Base: `538b476e` (post-#27 WASM Component Tool substrate).
Requirement ids: B1–B11 (issue body), C1–C8 (review comments), A1–A4 (automation
comment). This document is also the B9/C7/A4 **legacy deletion gate** record (§11).

## 1. Goal

One unified Cell authorization and evidence path for Component Tools:

1. **Admission** (per tool, per policy): profile-approved capability set +
   boundary policy decide whether a Tool is callable and with which admitted
   capabilities.
2. **Per-operation authorization** (per Tool Host import): the operation's
   host-bound Cell must be covered by an admitted capability and its concrete
   target must pass host-owned scope validation; failure rejects only that
   operation and never invokes the host facility.
3. **Result accounting**: the result Cell must be inside the descriptor ceiling
   and the admitted set; the host deterministically measures context
   allocation. Tool self-reported numbers are never trusted (no numeric channel
   exists; the host measures `value_json` itself).
4. **Evidence separation**: admission evidence ("why authorized" — approved
   capability names) and observed effects ("what happened" — Cell + ObservedScope
   + concrete target) are recorded in separate, versioned, canonically-ordered
   structures through audit, provenance, and attestation.

Out of scope (issue): migrating production Tools, adding Tool Host channels,
changing the accepted Cell model (`CtxOp × ExtImpact × scope × trust`).

## 2. Decisions (settles synthesis §3 items 1–14)

| # | Decision |
|---|---|
| D1 | Admission evidence is produced **per call** by the runtime admission step and recorded in the call's evidence ledger; the native transport merges it into audit metadata as a field separate from observed effects. (A2 per-call attribution.) |
| D2 | "Per-operation" today gates `host-control.emit-event`, the only effectful import. Host-side binding: `emit-event ⇒ Cell{Mutate, None, <tool's session-channel scope>}` with concrete target `Session{turn_id}`; a tool with **zero or more than one** session-channel scope gets the operation denied (fail-closed binding — no guessing). `cancelled` is a pure control probe: no Cell, never gated, documented as such. The per-op authorizer is a general seam (`authorize_operation(cell, target)`) unit-tested with Path/Host targets even though no domain channel exists yet. The noop manifest gains a second capability `session.events = mutate:none:session` so the e2e allow path is expressible (the noop guest already calls `emit_event` unconditionally — no guest/wasm change). |
| D3 | Result-cell ceiling stays **exact set membership**: `result_cell ∈ descriptor.capabilities.cells ∩ admitted.cells`. No partial order over `CtxOp × ExtImpact` this cycle; drift tests pin exact membership. |
| D4 | Wire keeps a **single result cell** (no WIT/ABI break, respects the frozen Cell model). Multi-effect representation lives in evidence/audit: a versioned canonical **array** of observed effects (result effect + per-operation effects). |
| D5 | Legacy vocabulary is **not deleted** this cycle (§11). The unified path becomes the sole authorization for Component Tools; native methods stay on the legacy path behind the existing `transport.rs` dispatch split, which is the documented migration seam, not a second path for the same consumer. |
| D6 | Profile placement: new typed sub-table `[lancet.capabilities]` with `deny: Vec<String>` and `max_boundary: "in"|"ex"` (default `"in"`). `schema_version` stays 1: no released binary composes Component Tools (production `EXPECTED_COMPONENTS` is empty), so old-binary fail-open skew is unreachable; recorded as accepted risk. Top-level `[capabilities]` remains forbidden. Stage-1 capability grant derives from `[tools].enabled` (the enabled tools' declared capabilities) — there is no independent capability allow-list, so profiles stay portable. |
| D7 | Capability name grammar: lowercase dotted, `^[a-z0-9_-]+(\.[a-z0-9_-]+)+$` (≥ 2 segments), enforced in `ToolDescriptor::validate` (new `DescriptorError::InvalidCapabilityName`). Cross-tool rule at composition: same name → **identical Cell required**, else `CompositionError::CapabilityConflict` refuses startup; same name + same cell is a permitted shared alias. |
| D8 | Attestation: new predicate type v2 constant (v1 stays intact for stored rows). The statement builder is already shared (`build_in_toto_statement`, called by both `lancet-api/attestation.rs` and `provenance/projector.rs`); v1/v2 selection happens **inside** that builder at **turn granularity**: (1) replaying a turn with a stored attestation reproduces the stored `predicate_type` byte-identically (the `attestation_records.predicate_type` column is the authority); (2) a newly attested turn emits **v2 iff any audit row in the turn carries versioned evidence metadata**, else v1 — so native-only turns keep emitting v1 until they migrate, and old statements are never silently re-emitted as v2; (3) in a mixed turn the v2 statement carries the legacy materials unchanged plus per-call evidence entries (admission/observed effects/denials) only for the calls that produced them. v2 statement bytes = `canonical_json`; **v1 rows are never re-serialized canonically** (their stored bytes stand). |
| D9 | New wire types keep the issue vocabulary (`ObservedEffect`, `ObservedTarget`) and live in `lancet_protocol::evidence`. Files that also touch `lancet_authority::effect::ObservedEffect` alias the legacy import (`as LegacyObservedEffect`). Authority types are unchanged this cycle. |
| D10 | `EffectErrorData` keeps legacy fields untouched; Component-path violations populate `CepError.message` (the struct has no message field) and new **optional** `cell_ref`/`capability` fields (`skip_serializing_if = Option::is_none`, additive; the struct is not `deny_unknown_fields`, and error data is never persisted — audit stores only the code + a message summary). **No new `CepErrorCode` or `PolicyReasonCode` variants**: admission denials (stages 1–3) reuse `CapabilityNotGranted`; result-cell violations keep `EffectViolation`; per-operation denials produce no wire error at all (silent drop + recorded evidence). Both enums stay closed with their pinned unknown-variant rejection tests. |
| D11 | Capability admission is **unconditional** for Component Tools: `CepMode` warn/off gates only the legacy effect-region checks (per lancet-authority AGENTS.md). Warn mode does not soften Cell admission. |
| D12 | Deterministic context measure: `context_measure = len(canonical_json(result_value))` in bytes, computed host-side and recorded on **every** result `ObservedEffect`, whatever `ctx_op` the guest declared. Rationale: branching on the declared `ctx_op` made metering guest-switchable — the shipped `noop.echo` declares an `alloc` *and* a `mutate` capability over one scope and both are normally admitted, so relabelling the result cell passed the ceiling and suppressed the measure. The declared `ctx_op` states charge intent for #6's charge/credit seam only; it never gates measurement. Operation effects stay `None` (they return no value). `Free` remains vocabulary-only (no seam); budget *enforcement* stays in #6 — #28 delivers measurement + recording only. |
| D13 | Concrete-target matching semantics for Path/Host stay with the future domain channels; the per-op authorizer denies Path/Host targets outright this cycle (fail closed — no channel can legitimately produce them). Session targets must equal the call's `turn_id`. Recorded so the authority-policy suffix-vs-exact host question is a conscious later decision. |
| D14 | Guest-side cell strings remain duplicated literals this cycle (noop hardcodes its result CellRef); a no_std vocabulary crate is future work. |

## 3. Protocol additions (`lancet-protocol`) — B1, C1, C4

New module `src/evidence.rs`, re-exported (with `cell::*`) at the crate root:

```rust
/// Concrete runtime target of an observed effect (C1).
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ObservedTarget {
    Path { path: String },
    Host { host: String },
    Session { turn_id: String },
    None,
}

/// Scope identity resolved by the host from the descriptor (never guest-supplied).
pub struct ObservedScope {
    pub scope: String,        // descriptor scope name (the scope_id)
    pub trust: Trust,         // host-derived, fail-closed
    pub channel: ChannelKind,
}

pub enum EffectSource { Operation, Result }

/// One actual effect (B5: real Cell + ObservedScope + target; never a capability alias).
pub struct ObservedEffect {
    pub seq: u32,                       // per-call total order
    pub cell: Cell,
    pub scope: ObservedScope,
    pub target: ObservedTarget,
    pub source: EffectSource,
    pub context_measure: Option<u64>,   // D12; host-computed on every result effect
}

/// Versioned canonical container (C1/C8): effects sorted by seq.
pub struct ObservedEffectsRecord { pub version: u32 /* 1 */, pub effects: Vec<ObservedEffect> }

/// A per-operation denial (recorded evidence, distinct from actual effects).
pub struct DeniedOperation { pub seq: u32, pub cell: Cell, pub scope: ObservedScope,
                             pub target: ObservedTarget, pub reason: String }

/// Why the call was authorized (B5/A2/B6).
pub struct AdmissionEvidence {
    pub version: u32,                    // 1
    pub tool: String,
    pub approved_capabilities: Vec<String>, // sorted+deduped capability names
    pub denied_capabilities: Vec<String>,   // subtracted by profile deny (sorted)
    pub boundary_excluded: Vec<String>,     // removed by max_boundary (sorted)
    pub max_boundary: Trust,
}

/// Operator/profile admission inputs (shared wire vocabulary for policy — B1).
pub struct CapabilityAdmissionPolicy {
    pub enabled_tools: BTreeSet<String>,
    pub denied_capabilities: BTreeSet<String>,
    pub max_boundary: Trust,             // Default::default() == Trust::In here — see below
}
```

`CapabilityAdmissionPolicy::default()` is fail-closed: empty `enabled_tools`,
empty deny, `max_boundary = Trust::In`. Note the deliberate asymmetry with
`Trust::default() == Ex` (scope trust fail-closed = ex; boundary fail-closed =
in). Implement via explicit `Default`, not derived.

**Persistence convention**: the evidence types above are stored inside audit
`metadata_json` and attestation predicates, so they follow the persisted-root
convention from `provenance.rs` — **no `deny_unknown_fields`** on
`ObservedTarget`/`ObservedEffect`/`ObservedEffectsRecord`/`AdmissionEvidence`/
`DeniedOperation` (forward-compat readers), with the `version` field checked
before interpreting contents.

Also in protocol:
- `DescriptorError::InvalidCapabilityName` + grammar check in `validate()` (D7).
  Check order is fixed and documented: **grammar first** (per capability), then
  duplicate names, then undeclared scope — and the existing single-segment test
  fixtures (`ghost-read`, `todo-write`) are renamed to conforming dotted names
  so their asserted error variants stay meaningful.
- v2 attestation predicate constant beside v1 (C4) in `provenance.rs`.
- Canonical JSON writer `canonical_json(&serde_json::Value) -> String`
  (explicitly sorted object keys — never relying on `serde_json::Map`
  internals or the absence of `preserve_order` — no whitespace, serde_json
  number formatting) in a new `src/canonical.rs` — the single implementation
  used by the context measure (D12) and v2 attestation statement bytes (D8).
  Property-test: key-order invariance and array-order stability.

## 4. Admission algorithm (`lancet-runtime/src/admission.rs`) — B2, B4, C2, C3

Pure function, the **only** authorization authority for Component Tools:

```rust
pub fn admit_tool(desc: &ToolDescriptor, policy: &CapabilityAdmissionPolicy)
    -> Result<AdmittedTool, AdmissionDenial>
```

Fixed stage order (C3):
1. **enabled**: `desc.name ∈ policy.enabled_tools`, else `ToolNotEnabled`.
   Start from the enabled tool's declared capabilities.
2. **profile deny/subtract**: remove capabilities whose name ∈
   `policy.denied_capabilities`; record removed names.
3. **max_boundary predicate**: if `policy.max_boundary == In`, remove every
   capability whose cell scope trust resolves to `Ex` (`trust_of`, undeclared →
   `Ex` — B4); record removed names. If the remaining set is empty →
   `NoAdmittedCapabilities` (tool fails closed, B2).
4. **per-operation concrete check** happens at call time (§5), not here.

Output: `AdmittedTool { descriptor: Arc<ToolDescriptor>, admitted: BTreeMap<String, Capability>, evidence: AdmissionEvidence }`
(the descriptor rides along so `host.rs` can resolve operation cells from
`StoreState` alone). `AdmissionDenial` **also carries an `AdmissionEvidence`**
(approved set empty or partial, plus the denial reason) so a denied call's
audit record still gets its "why" evidence — the deny finalize must not follow
the legacy pre-dispatch-deny pattern of `metadata = None`. A stage-1
`ToolNotEnabled` denial carries evidence with an empty approved set.

**Unknown profile capability** (C2, defined behavior):
`validate_policy_against_catalog(catalog, policy)` errors on any
`denied_capabilities` name that matches no composed capability name — a deny
typo silently keeping a grant is a fail-open hazard, so it is a hard startup
error. It runs at the single point where catalog and policy first coexist:
server construction (`ServerRunConfig` assembly in lancet-api / the native
server spawn), not per turn. Documented in the profile schema docs (§8),
including the consequence: on a binary whose component catalog is empty
(today's production `EXPECTED_COMPONENTS = &[]`), **any** deny entry is a
startup error.

**Cross-tool conflict** (C2): `ClosedCatalog::compose` builds a global
`name → Cell` map; same name with a different cell →
`CompositionError::CapabilityConflict { name, tools }` (startup refusal).

Admission runs per call in the native transport before `rt.call` (D1): denied →
respond with the policy error, audit the denial with its `AdmissionEvidence`
(what was approved/subtracted/excluded), never enter Wasmtime.

## 5. Per-operation authorization + result path (`lancet-runtime`) — B3, B7, A1, A3

`ComponentCallContext` gains `admitted: Arc<AdmittedTool>` (or equivalent).
`host.rs::emit_event`:

1. Resolve the operation cell per D2 (host-side table; guests never name cells
   for operations).
2. `authorize_operation(&admitted, &descriptor, cell, target)`:
   - covered: some admitted capability's cell == operation cell (exact);
   - target check: `Session{turn_id}` must equal the call's turn id;
     `Path`/`Host` targets are denied outright this cycle (D13).
3. Denied → do **not** call `events.emit_sync` (host facility untouched),
   record `DeniedOperation`, return unit (WIT `emit-event` has no return; the
   call itself continues — only the operation is rejected, B3).
4. Allowed → invoke the sink and record `ObservedEffect { source: Operation,
   cell, scope, target: Session{turn_id}, context_measure: None }`.

Result path in `runtime.rs::call` (order):
parse trust-free CellRef → declared-membership check (existing) → **admitted**
membership check (new; violation = fail closed: the call errors and the
**result** effect is never recorded — but operation effects already recorded
in the ledger survive into audit, as B5 requires) → result-schema validation →
`context_measure = len(canonical_json(value))` **unconditionally**, whatever
`ctx_op` the guest declared (D12: a declared `Free` or `Mutate` result cell is
accepted and recorded as an effect carrying its measure; the declared `ctx_op`
only tells #6 how to charge it) → record result `ObservedEffect { source:
Result, target: Session{turn_id} }` with host-derived trust.

The evidence ledger becomes:

```rust
struct EvidenceInner { seq: u32, admission: Option<AdmissionEvidence>,
                       effects: Vec<ObservedEffect>, denied: Vec<DeniedOperation> }
```

`snapshot()` returns `(AdmissionEvidence, ObservedEffectsRecord, Vec<DeniedOperation>)`;
`snapshot_keys()` (4-segment audit keys) remains, derived from `effects`, so the
existing `component_cells` audit key keeps working.

## 6. Audit / provenance / attestation (`lancet-audit`) — B5, B6, C1, C4, C5, C8, A2

1. **Schema versioning first (C5)**: stamp `PRAGMA user_version = 1` on
   create/empty-open. On open, three cases:
   - `user_version > 1` → fail with explicit action ("newer audit schema;
     upgrade the binary or move the DB aside");
   - `user_version == 0` with a **compatible** current-shape table (verified
     via `PRAGMA table_info` against the expected column set) → stamp to 1
     (additive change, no data loss);
   - `user_version == 0` with an **incompatible** table shape (the real
     old-schema case C5 demands detection for — today it opens silently and
     dies later on a raw INSERT error) → fail with an explicit action message
     naming the DB path and the remedy (move it aside / use the read-only
     import), never touching the data.
   The existing read-only legacy import path stays intact; a test synthesizes
   an old-shape DB (e.g. missing a NOT NULL column) and asserts the actionable
   error. No history is ever silently dropped.
2. **Structured evidence**: `merge_finalize_metadata` gains
   `admission_evidence` (versioned object) and `observed_effects` (versioned
   canonical array, seq-ordered) and `denied_operations`; `component_cells`
   stays (derived) for query compatibility; `observed_capabilities` ordering
   becomes deterministic (BTreeSet).
3. **Attestation v2 (C4/D8)**: v2 predicate carries `admission` ("why") and
   `observed_effects` (+ `denied_operations`) ("what") as separate per-call
   fields; version selection at turn granularity per D8 (stored
   `predicate_type` wins on replay; new turns → v2 iff any row carries
   versioned evidence); v2 statement bytes = `canonical_json`;
   `canonicalize.rs` comparison descends into arrays.
4. **Provenance projection (B6)**: the projector's activity-attribute
   whitelist gains the three new metadata keys (`admission_evidence`,
   `observed_effects`, `denied_operations`) so "why" and "what" survive into
   the PROV fragment as separate attributes; the pinned whitelist test is
   consciously updated, and the B11 e2e asserts the replayed PROV activity for
   a component call carries both admission and observed-effect attributes.
5. Audit query API: existing filters untouched; add structured accessors only
   if cheap — C8 is satisfied by the versioned array + stable ordering +
   attested fields, not by new SQL columns.

## 7. Native transport (`lancet-native`) — plumbing only

`transport.rs` Component branch: run admission (per D1) → on deny, finalize
audit with admission evidence + policy error; on admit, build
`ComponentCallContext` with admitted set + ledger; after the call, merge
admission evidence + observed effects + denials into finalize metadata.
Legacy pre-dispatch deny and all native handlers untouched (D5/C7).
`ServerRunConfig`/`cep_context.policy` carries `CapabilityAdmissionPolicy`:
the field is added to `lancet_authority::policy::CepPolicyConfig` (authority
already depends on protocol; no cycle — runtime depends on protocol only).
`defaults_for_project_root` and every other `CepPolicyConfig` constructor
initialize the new field fail-closed (`CapabilityAdmissionPolicy::default()`);
the struct's `PartialEq`/`Eq` derives are satisfied by the new field's types.

## 8. Profile (`lattice-profile`) — B8, C2, C3

New typed sub-table (D6):

```toml
[lancet.capabilities]
deny = ["session.events"]   # capability names; unknown names → startup error
max_boundary = "in"          # default "in"; "ex" opts into external-world capabilities
```

- Typed `LancetCapabilitiesSection` added to `LancetSection`'s known sub-tables
  (custom Deserialize) so it can never fall into `extensions` silently.
- Schema docs state: unknown deny name = hard error at server startup (C2) —
  including on binaries whose component catalog is empty, where any deny entry
  therefore fails startup; `max_boundary` default `in`; stage order per §4.
- `[lancet.effects].deny` (`ImpactKind`) remains for the **native** path only
  (D5); its removal is gated in §11. B8 is delivered for the unified path:
  Component-tool policy uses capability names + boundary + concrete scope rules
  and never touches method-impact identity.
- lattice-api turn setup translates profile → `CapabilityAdmissionPolicy`
  (`enabled_tools` from `[tools].enabled`).

## 9. Toolchain (`tools/noop`, `xtask`) — D2, C6

- `tools/noop/manifest.toml`: add `[[capabilities]] name = "session.events",
  cell = "mutate:none:session"`. WIT and guest code unchanged → component
  bytes, `[artifact]` sha256, and the footprint pin all unchanged (the guest
  already calls `emit_event`; no `tool-build` needed).
- Run `cargo xtask tool-codegen`: it rewrites **only**
  `tools/noop/generated/{tool-host.wit, descriptor.json, llm-projection.json,
  registration.rs, expected-identity.json}` (here: descriptor.json + the
  `descriptor-sha256` field of expected-identity.json; the other three stay
  byte-identical). Then **hand-edit**
  `crates/lancet-runtime/src/generated/noop_fixture.rs` to copy the new
  descriptor sha — codegen never touches that file, the CI `--check` gate
  doesn't cover it, and forgetting surfaces as `DescriptorDrift` in every
  composition test. Same commit.
- xtask validates the new grammar via `descriptor.validate()`; the cross-tool
  `CapabilityConflict` check lives **only** in `ClosedCatalog::compose`
  (xtask processes one manifest at a time and cannot see conflicts).

## 10. Tests — B10, C6, B11

| Scenario | Where |
|---|---|
| capability alias (two names, one cell): admission per-capability, enforcement per-cell; alias names appear in admission evidence only, never as effects | lancet-runtime unit + protocol |
| partial capability admission: deny `session.events` → call succeeds, `emit_event` denied+recorded, sink never invoked | lancet-runtime `tests/call.rs` |
| uncovered operation: no `mutate:none:session` capability admitted → emit-event denied, host facility not called | lancet-runtime |
| default `ex` + `max_boundary=in`: undeclared-trust scope excluded at stage 3; tool with no remaining caps → not callable | admission unit tests |
| scope escape: authorizer denies Path/Host targets and mismatched session turn ids | admission/authorizer unit tests |
| result cell drift: undeclared result cell → violation, **no** result evidence recorded (prior operation effects survive) | lancet-runtime |
| effect ceiling: declared-but-not-admitted result cell → violation | lancet-runtime |
| `Free`/`Mutate` result cell: synthetic descriptor with `free:none:session` capability → result accepted, effect recorded **with its measure** — `Free`/`Mutate` differ from `Alloc` in how #6 charges the number, not in whether the host records one | lancet-runtime unit |
| result-cell relabelling: the real `noop.echo` capability set (`alloc` and `mutate` over one scope, both admitted) → a `mutate:none:session` result cell still passes the ceiling **and** is still measured (D12: metering is not guest-switchable) | lancet-runtime unit |
| `covers` exactness: a capability on one scope never covers the same `ctx_op`/`ext_impact` on another declared scope, nor a differing `ext_impact` — pinned on `covers`, `authorize_operation`, and `check_result_cell` | lancet-runtime unit |
| C3 stage order: a capability that is both name-denied and `ex`-trust is attributed to `denied_capabilities`, never to `boundary_excluded` | admission unit tests |
| D2 unbound binding: a descriptor with zero or two session-channel scopes → `emit-event` denied under the `<unbound>` sentinel, sink never reached | lancet-runtime `host.rs` unit |
| old-shape audit DB (missing NOT NULL column) → actionable open error, data untouched | lancet-audit |
| audit attribution: `admission_evidence` vs `observed_effects` vs `denied_operations` distinct fields; `component_cells` intact | lancet-audit + lancet-native round trip |
| attestation determinism: stored vs re-projected v2 statements byte-identical; array order stable; v1 rows still project as v1 | lancet-audit + lattice-api |
| C6 conflict: same capability name → different cell refuses startup | lancet-runtime `tests/gate.rs` |
| C6 multi-capability tool + multi-effect call: noop emits event + result → 2 effects in one call, ordered by seq | round-trip tests |
| C6 unknown profile capability: deny typo → startup error | policy validation tests |
| B11 e2e: noop allow / deny(result cap) / partial-deny(events cap) with audit + provenance + attestation assertions | lancet-native `cep_component_round_trip.rs`, lattice-api |

Pinned tests that consciously change (complete list — anything else breaking
is a regression):

- `lancet-protocol/src/cell.rs`: fixture capability names `ghost-read`/
  `todo-write` renamed to dotted grammar-conforming names (asserted error
  variants unchanged; grammar is the first per-capability check).
- `ComponentCallContext` construction sites (exactly 3: native
  `transport.rs:224`, lancet-runtime `tests/call.rs`, `tests/negative.rs`)
  gain the admitted set.
- **Every test built on `ServerRunConfig::in_memory_for_tests()` + the noop
  fixture** (lancet-native `cep_component_round_trip.rs`, lattice-api
  `turn/cep_tests.rs` component tests, …) must thread
  `enabled_tools = {"noop.echo"}` through the new policy field — the
  fail-closed default otherwise flips them all to `ToolNotEnabled`.
- `component_cells` assertions grow from one key to two
  (`mutate:none:session:in` operation + `alloc:none:session:in` result) in
  `cep_component_round_trip.rs` and lancet-runtime `tests/call.rs`.
- Projector whitelist pin (`projector.rs:362-396`) gains the three new keys.
- `lattice-api tests/audit_turn_provenance.rs` and lancet-native
  `tests/prov_isomorphism.rs` change where component turns now emit v2
  statements / new metadata.
- Profile forbidden-section tests keep passing unchanged (top-level
  `[capabilities]` still rejected); footprint pin unchanged (wasm untouched).

The lattice-api B11 leg follows the existing `cep_tests.rs` pattern
(lancet-runtime test-utils noop fixture + `ServerRunConfig` with `components`
+ the new `CapabilityAdmissionPolicy`) — `lancet-api`'s `spawn_composed` is
`pub(crate)` and is not the seam.

## 11. Legacy deletion gate (B9 / C7 / A4 — recorded, not executed)

`ImpactKind`, `EffectScopeKind`, `OverlayBehavior`, `FailureSemantics`,
`BaseAuthorityRequirement` are **not** deleted in #28. They sit on four
serialized surfaces (JSON-RPC error wire, stored provenance/attestation v1,
audit DB `impact_kind` values, profile `[lancet.effects]`) and the legacy path
is still the only authorization for the five native methods. Deletion requires
**all** of:

1. Native methods migrated to Component Tools (ADR 0004 catalog absorption).
2. Audit rows/attestations produced only via the v2 evidence path; v1 kept
   read-only.
3. Profile `[lancet.effects]` retired in favor of `[lancet.capabilities]` for
   every consumer (lattice-system-harness direct_mutation coverage included).
4. `lancet-cep`/`lancet-catalog` retirement per ADR 0004.

Until then the `transport.rs` dispatch split is the single, documented seam
between the two vocabularies; Component Tools never consult the legacy types
(no dual authorization for the same consumer), and no new consumer of the
legacy vocabulary may be added.
