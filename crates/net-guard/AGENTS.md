# AGENTS.md

Crate-local guidance for `net-guard`.

## Role

This crate is the neutral Layer-0 leaf for URL validation, DNS resolution,
private/reserved address rejection, and DNS-rebinding-domain checks shared by
LATTICE and LANCET.

## Boundaries

- Keep the public interface limited to URL/DNS safety behavior and neutral
  errors.
- Depend only on `std` and `url`; do not add tokio, reqwest, LANCET, or LLM
  types.
- Keep network policy, HTTP client construction, provider behavior, and native
  fetch execution in their owning crates.
- Preserve request-time DNS pinning semantics in callers.

## Verification

- Local: `cargo test -p net-guard`.
- Dependency gate: `cargo tree -p net-guard --depth 1`.
- Cross-crate changes: run the affected LLM, authority, and native tests.
