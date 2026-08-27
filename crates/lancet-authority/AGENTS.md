# AGENTS.md

Crate-local guidance for `lancet-authority`.

## Role

This crate owns host-side policy semantics: capability matching, network and
workspace policy, observation budgets, Guard L1, effect regions/checking, and
boundary-credit validation. It performs no Tool I/O.

## Sub-architecture

- `src/policy/network.rs` validates URL policy before network I/O and owns the
  single `allowed_hosts` matcher. `lancet-runtime::host_network` consumes it,
  then independently validates every resolved address before connecting.
- `src/policy/observation.rs` owns observation bounds and derives the shared
  per-Turn budget limits.
- `src/policy/default.rs` holds profile/external-policy configuration plus the
  residual legacy native decision hook. The production native catalog is empty
  after #32, so built-in Tools do not resolve through this hook.
- `src/policy/resolved.rs` resolves only generic native catalog methods. All five
  built-ins are intentionally unknown to it.
- `src/policy/visibility.rs` owns read visibility and System Path Exclusion
  semantics consumed by the runtime workspace host.
- `src/guard.rs` owns Guard L1 path and scope verdicts.
- `src/effect/` owns the residual legacy effect region/checker/ledger types.
- `src/boundary_credit.rs` validates isolation boundary evidence.

## Network policy

`fetch.web` is fail-closed unless all applicable gates agree:

- the Tool is enabled and admitted at `Trust::Ex`;
- `allow_public_https = true`;
- the normalized host matches non-empty `allowed_hosts`;
- localhost additionally has `allow_localhost = true` and an exact allowlist
  match;
- URL, DNS result, redirect target, and connected peer pass the reserved/private
  address rules.

`host_allowed_by_list` is the only host-pattern matcher. It trims and
ASCII-lowercases patterns; a leading dot matches the apex and subdomains. Empty
or dots-only patterns match nothing. Keep URL policy and runtime host-target
scope checks on this function.

## Boundaries

- Keep decisions deterministic and I/O-free. DNS, sockets, TLS, redirects, and
  response shaping belong to `lancet-runtime`.
- Components declare requested Cells; host admission and each Tool Host import
  remain the authority.
- Do not let LLM tool exposure grant authority.
- Keep audit persistence in `lancet-audit` and server lifecycle in
  `lancet-native`.
- Add policy readers with their config keys. Reject unsupported keys rather than
  accepting values that execution ignores.
- Retire legacy envelope/base-authority/effect vocabulary only when the root
  `AGENTS.md` preconditions all hold.

## Verification

- Local: `cargo test -p lancet-authority`.
- Network changes: also run `cargo test -p lancet-runtime --lib host_network::`
  and the `lancet-native` `fetch_web` Component round trips.
- Guard/effect changes: run both accepted and rejected integration cases.

## Record maintenance

Update this file when policy ownership moves. Record landed fixes in root
`FIX.md`; keep only open residuals in `FINDINGS.md`.
