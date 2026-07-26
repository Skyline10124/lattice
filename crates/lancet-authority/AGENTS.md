# AGENTS.md

Crate-local guidance for `lancet-authority`.

## Role

This crate owns CEP authority semantics: default policy, capability matching,
network/observation/budget policy, Guard L1 path/scope checks, effect-region
data, effect checking, effect ledger helpers, and boundary-credit validation.

## Sub-Architecture

- `src/policy/mod.rs` ties policy modules and exported config together.
- `src/policy/default.rs` resolves default policy decisions.
- `src/policy/capability.rs` checks required capabilities.
- `src/policy/network.rs`, `observation.rs`, and `budget.rs` own
  domain-specific policy decisions. Command policy was permanently retired with
  `exec.command_run` (issue #4); do not reintroduce `policy/command.rs` or
  `shell_ast.rs`.
- `src/policy/resolved.rs` owns merged/resolved policy state.
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

- FIXED M-24-04: catalog descriptors declare method-local base authority
  requirements applied through `lancet-cep::BaseAuthorityPlan`; this crate
  remains the policy/guard/effect authority implementation.
- FIXED #24: declared denied effect kinds are rejected before handler dispatch;
  observed-effect checks remain post-dispatch defense in depth.
- WATCH M-24-01: host capability semantics must align with protocol field names
  and native effect-region matching.
- WATCH: `mode = off` skips effect-region checks only; policy, capability,
  effect-kind denial, and native routing remain active.
- No current root finding is isolated solely to this crate.

## Next Actions

- Keep base-authority grant expansion outside this crate unless it becomes a
  policy decision; this crate should validate capabilities, policy, guard, and
  effect semantics.
- Keep host allow semantics synchronized across protocol capabilities, network
  policy, native web fetch, and effect-region checks.
- Add policy coverage whenever new decision reasons or capability sources are
  introduced.
- Preserve Guard L1 path traversal and project-root escape tests for every path
  handling change.
- Retire legacy envelope/base-authority/effect-region vocabulary only after all
  behavior consumers use the Cell path; do not run permanent dual authority
  models.

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
