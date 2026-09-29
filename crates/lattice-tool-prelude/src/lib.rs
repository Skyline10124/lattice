// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Guest-side SDK for LATTICE Source Tools (ADR 0004).
//!
//! This crate is the *only* API surface a Source Tool guest depends on. It
//! re-exports the pinned `wit_bindgen` bindings generator and provides the
//! Component export glue plus descriptor/Cell helpers. It deliberately exposes
//! no host internals: there is no `lancet-*` dependency, no policy/effect/
//! audit surface, and no workspace/network/session operation. A guest Tool
//! affects the world only through its Tool Host WIT imports.
//!
//! Guest Tools target `wasm32-unknown-unknown` (not `wasm32-wasip2`): pure
//! computation plus Tool Host imports keeps the import whitelist exactly equal
//! to the Tool Host world, so the slice-C "no ambient WASI" gate is trivial.

#![no_std]
extern crate alloc;

/// Pinned re-export of the WIT bindings generator. Guests call
/// `lattice_tool_prelude::wit_bindgen::generate!({ path: "generated" })` inside
/// a `mod bindings { ... }` block and never depend on `wit-bindgen` directly,
/// so every Tool's Component bindings come from one fixed toolchain.
pub use wit_bindgen;

/// Wire a guest Tool's `tool-api` export to its `Guest` implementation.
///
/// Convention: the guest crate generates bindings into a top-level module
/// named `bindings` (see `lattice_tool_prelude::wit_bindgen::generate!`) and
/// invokes this macro **inside that module**, after `generate!`, with the
/// guest type imported into the module scope:
///
/// ```ignore
/// mod bindings {
///     lattice_tool_prelude::wit_bindgen::generate!({ path: "generated" });
///     use crate::MyTool;
///     lattice_tool_prelude::export_tool!(MyTool);
/// }
/// ```
///
/// `wit-bindgen`'s generated `export!` is `pub(crate)` and its inner macro
/// resolves `self` at the invocation site, so the export must be wired from
/// within the `bindings` module (where `generate!` ran), not the crate root.
/// This macro is a thin, documented wrapper over that generated `export!`.
///
/// The export is emitted only for `wasm32`. Guest crates are workspace
/// members, so `cargo build --workspace` also links them as host cdylibs;
/// the Component export names contain `:` (`lattice:tool/...`), which the
/// host ELF version script cannot express and rust-lld rejects. The host
/// build still type-checks the guest and runs its unit tests.
///
/// Host internals are not reachable from this crate.
#[macro_export]
macro_rules! export_tool {
    ($ty:ident) => {
        #[cfg(target_arch = "wasm32")]
        export!($ty);
        // Keeps the guest's `use crate::Tool;` in `bindings` used on host.
        #[cfg(not(target_arch = "wasm32"))]
        const _: ::core::marker::PhantomData<$ty> = ::core::marker::PhantomData;
    };
}
