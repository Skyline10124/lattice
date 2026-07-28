# AGENTS.md

Crate-local guidance for `lattice-tool-prelude`.

## Role

Guest-side SDK for LATTICE Source Tools (ADR 0004). The *only* API surface a
Source Tool guest depends on. It re-exports the pinned `wit_bindgen` bindings
generator and provides the `export_tool!` Component export glue. It exposes no
host internals: no `lancet-*` dependency, no policy/effect/audit surface, and
no workspace/network/session operation.

## Boundaries

- `#![no_std]` + `extern crate alloc`. Guests target `wasm32-unknown-unknown`
  (not `wasm32-wasip2`) so the import whitelist equals the Tool Host world and
  the no-ambient-WASI gate is trivial.
- `pub use wit_bindgen;` pins the bindings toolchain for every Tool. Guests
  also depend on `wit-bindgen` directly because `generate!` emits
  `wit_bindgen::rt::...` by bare crate path; keep both at the same version.
- `export_tool!` must be invoked **inside** the guest's `mod bindings` (where
  `wit_bindgen::generate!` ran): wit-bindgen's generated `export!` resolves
  `self` at the call site, so the export cannot be wired from the crate root.
- Never add a `lancet-*` dependency here; that would leak host internals into
  the guest trust boundary.

## Source Tools

- `tools/noop` (`distribution = "fixture"`) is the tracer, composed only by the
  `test-utils` fixture. `tools/todo-write` (`distribution = "builtin"`) is the
  first production Tool (#29): its manifest owns the whole contract, and all of
  its validation lives in the params schema so the host refuses a bad call before
  the Component is entered. A guest must not carry a second, imperative copy of
  the descriptor's rules.

## Verification

- Local: `cargo test -p lattice-tool-prelude`.
- Boundary: `cargo tree -p lattice-tool-prelude` must show no `lancet-*`.
- Guest build: `cargo build -p <tool> --target wasm32-unknown-unknown` (driven
  by `cargo xtask tool-build`).

## Record Maintenance

- Update this file in the same patch that changes the prelude's role or
  boundary contract.
- Mirror landed fixes in root `FIX.md` and open/residual items in root
  `FINDINGS.md`.
