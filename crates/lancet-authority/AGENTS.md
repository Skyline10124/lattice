# AGENTS.md

Crate-local guidance for `lancet-authority`.

## Role

This crate owns CEP authority semantics: default policy, capability matching,
network/observation/budget policy, Guard L1 path/scope checks, effect-region
data, effect checking, effect ledger helpers, and boundary-credit validation.

## Sub-Architecture

- `src/policy/mod.rs` ties policy modules and exported config together.
- `src/policy/default.rs` resolves default policy decisions.
- `src/policy/capability.rs` holds `InvocationFacts` and
  `SessionStateOperation`. Its capability *checking* left with the envelope path
  (`ba3a8e27`); required capabilities come from catalog descriptors.
- `src/policy/network.rs`, `observation.rs`, and `budget.rs` own
  domain-specific policy decisions. Command policy was permanently retired with
  `exec.command_run` (issue #26, Codeberg #4); do not reintroduce
  `policy/command.rs` or `shell_ast.rs`. `network.rs` owns the one host
  allow-matcher (`host_allowed_by_list`); `observation.rs` owns the one
  observation limit set and `budget.rs` is its derived turn-scoped projection.
- `src/policy/resolved.rs` resolves a `PolicyInput` into lexical facts. It
  carries only the project root — limits live in `PolicyConfig`, where the
  decision path reads them.
- `src/guard.rs` owns Guard L1 path and scope verdicts.
- `src/effect/region.rs`, `checker.rs`, and `ledger.rs` own effect regions and
  checking.
- `src/boundary_credit.rs` validates isolation boundary evidence.

## Boundaries

- No native shell/filesystem/web execution or handler lifecycle code.
- No audit row persistence; native lifecycle writes audit through
  `lancet-audit`.
- Keep policy decisions deterministic and explicit about allow/warn/deny/action.
- Do not let LLM tool exposure become the authority model.
- ADR 0004 keeps authority host-side: admission and each Tool Host operation must
  derive from protocol Cell/capability/scope data. Components never become the
  policy authority.

## Recent Issues

- ADDED #30: `observe.workspace` is an embedded Component Tool; this crate's
  per-method observation resolution went with it. `policy/resolved.rs` no
  longer extracts a per-mode observation path (`observe.workspace` resolves
  as an unknown method, pinned by
  `resolve_observe_workspace_errors_as_unknown_method`), and
  `policy/default.rs` dropped its WorkspaceObservation arm. What stays is the
  *input* side the Tool Host consumes: `[lancet.workspace]` visibility
  (included-ignored / denied-read patterns) and the `ObservationPolicyConfig`
  limit set, now read by `lancet-runtime`'s `workspace-observe` host imports
  rather than by a native handler.
- ADDED #28: `CepPolicyConfig` carries `capability_admission`
  (`CapabilityAdmissionPolicy`), the Component Tool admission inputs the native
  transport reads per call. Every constructor initializes it fail-closed. This
  crate does not interpret it — Cell admission lives in `lancet-runtime` and is
  unconditional, so `CepMode` still gates only the legacy effect-region checks.
- FIXED M-24-04: catalog descriptors declare method-local base authority
  requirements applied through `lancet-cep::BaseAuthorityPlan`; this crate
  remains the policy/guard/effect authority implementation.
- FIXED #46 (Codeberg #24): declared denied effect kinds are rejected before
  handler dispatch; observed-effect checks remain post-dispatch defense in
  depth.
- FIXED #122: `policy::network::host_allowed_by_list` (with
  `normalize_host_pattern`) is the one `allowed_hosts` matcher. Network URL
  validation, `DefaultPolicyHook`, and `AuthorizedEffectRegion::contains` all
  call it; `authorized_region_for` stores `region.hosts` in comparison form.
  Semantics: trim, ASCII-lowercase, leading dot means suffix match including the
  apex domain, empty/dots-only patterns match nothing. Three implementations with
  two semantics previously made `.example.com` pass the policy layers and fail
  the region as an `EffectViolation`.
  What an *empty* `allowed_hosts` means stays per-layer on purpose: URL
  validation falls back to `allow_public_https`, while the region grants no host,
  so Enforce-mode `fetch.web` fails closed before outbound I/O. That asymmetry is
  pinned by the harness scenario
  `direct_tools_fetch_capability_denial_with_public_https`; do not converge it by
  loosening the region.
- FIXED #121: every configurable key in this crate has an execution-side reader.
  `ObservationPolicyConfig` is the one limit set and `BudgetLimits` is its
  two-field derived projection (`budget_limits()`), so the external policy file's
  `[budget]` (now a deprecated alias of `[observation]`) can no longer land in
  slots nothing reads. Deleted as write-only residue: `ObservationPolicyConfig::
  allowed_roots` + `resolve_observation_target` (read scope is the canonical
  project root plus `[lancet.workspace]` visibility), `MethodPolicy::
  {allowed_sources, required_capability, max_single_read_bytes}`,
  `CapabilitySourceClass`/`CapabilitySourcePolicy`/`PolicyConfig::
  capability_sources` and their TOML tables, and the four unread `PolicyResolver`
  fields. `MethodPolicy` keeps `enabled` + `max_todo_ops`, both with readers.
- WATCH: `PolicyReasonCode::CapabilitySourceNotAllowed`
  (`lancet-protocol/src/policy.rs`) has no producer since `ba3a8e27` — the same
  envelope residue as #121, but protocol-stable serialized surface, so its
  removal is a separate decision.
- WATCH: `mode = off` skips effect-region checks only; policy, capability,
  effect-kind denial, and native routing remain active.
- No current root finding is isolated solely to this crate.

## Next Actions

- Keep base-authority grant expansion outside this crate unless it becomes a
  policy decision; this crate should validate capabilities, policy, guard, and
  effect semantics.
- Any new host comparison must call `policy::network::host_allowed_by_list`. Do
  not hand-roll trim / lowercase / leading-dot handling — that is what #122 was.
- Every configurable policy key must have an execution-side reader, and any value
  duplicated across structs must be derived, not copied (#121). A key that is
  validated but unread is worse than an unsupported key: an unsupported key
  errors, an unread one silently leaves policy wider than the operator intended.
- Add policy coverage whenever new decision reasons are introduced.
- Preserve Guard L1 path traversal and project-root escape tests for every path
  handling change.
- Retire legacy envelope/base-authority/effect-region vocabulary only after all
  behavior consumers use the Cell path; do not run permanent dual authority
  models. The four deletion preconditions are in root `AGENTS.md` (full record:
  #28 design §11).

## Record Maintenance

- Update `Recent Issues` and `Next Actions` when policy, guard, effect
  region, or boundary-credit behavior changes.
- Record unresolved authority risks in root `FINDINGS.md`.
- Record landed authority fixes in root `FIX.md` with tests run.
- Keep cross-crate authority notes here only if they affect local edit strategy.
- Keep this file near 80 lines by replacing stale issue notes with current ones.

## Verification

- Local: `cargo test -p lancet-authority`.
- Policy/guard/effect changes: include property tests and integration tests in
  native/API paths.
- Boundary-credit changes should validate both accepted evidence and rejected
  unsupported credit claims.
