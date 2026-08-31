# AGENTS.md

Crate-local guidance for `lancet-authority`.

## Role

This crate owns deterministic host-side policy semantics: network and workspace
policy, observation budgets, Guard L1, read visibility, and System Path
Exclusion. Cell capability admission is owned by `lancet-runtime`; this crate
performs no Tool I/O.

## Sub-architecture

- `src/policy/network.rs` validates URL policy before network I/O and owns the
  single `allowed_hosts` matcher. `lancet-runtime::host_network` consumes it,
  then independently validates every resolved address before connecting.
- `src/policy/observation.rs` owns observation bounds and derives the shared
  per-Turn budget limits.
- `src/policy/default.rs` holds profile/external-policy configuration and
  workspace deny/read-visibility policy.
- `src/policy/visibility.rs` owns Policy-Visible Workspace read visibility decisions
  (ignored inclusion, read denial, system exclusion precedence) and System Path Exclusion
  semantics consumed by the runtime workspace host; read, list, grep, and find share the same decision.
- `src/guard.rs` owns Guard L1 path and scope verdicts.

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
- Keep audit persistence in `lancet-audit` and Component execution/lifecycle in
  `lancet-runtime`.
- Add policy readers with their config keys. Reject unsupported keys rather than
  accepting values that execution ignores.

## Verification

- Local: `cargo test -p lancet-authority`.
- Network changes: also run `cargo test -p lancet-runtime --lib host_network::`.
- Guard changes: run accepted and rejected runtime integration cases.

## Record maintenance

Update this file when policy ownership moves. Record landed fixes in root
`FIX.md`; keep only open residuals in `FINDINGS.md`.
