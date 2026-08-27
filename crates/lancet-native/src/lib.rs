// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! Transitional LANCET server and Rewind implementation.
//!
//! Production Agent Tools are embedded Components. Residual low-level executors
//! compile only for regression tests; `lattice-agent` cannot reach them.

// The retired native HashLine engine remains only as a regression-test asset.
// Production mutation execution lives in the embedded Components and
// `lancet-runtime`'s `workspace-mutate` host.
#[cfg(test)]
pub(crate) mod edit;
pub mod enforcement;
#[cfg(test)]
pub(crate) mod executor;
mod handlers;
mod server;

/// The concrete Local Workspace, consumed from `lancet-runtime`.
///
/// GitHub #30 moved the module down a layer: the `workspace-observe` Tool Host
/// imports run inside `lancet-runtime` and cannot reach up here. Rewind and the
/// retired edit engine's regression tests use the *same* concrete type through
/// this alias, so secure path resolution has exactly one home. It is a path
/// alias, not a seam — there is no trait behind it and nothing to substitute.
pub(crate) mod workspace {
    pub(crate) use lancet_runtime::workspace::*;
}

/// Explicit Rewind restore executor (GitHub #64).
///
/// Host-side operator surface: preflights every affected path against its
/// recorded post-state and restores parent-tree content with the same safety
/// primitives the mutation path uses. Not a CEP method — the Agent-visible
/// catalog gains nothing here. The implementation moved to `lancet-runtime` with
/// the rest of the workspace module in #30; this re-export keeps
/// `lancet_native::rewind` the stable name `lancet-api` already resolves.
pub mod rewind {
    pub use lancet_runtime::workspace::rewind::{
        RestoredPath, RewindPathConflict, RewindPathPreflight, RewindRestoreError,
        RewindRestoreOutcome, preflight_and_restore,
    };
}

#[cfg(feature = "inspect-scaffold")]
pub use handlers::register_inspect_scaffold_methods;
pub use lancet_dispatch::{CepClient, CepServer, CepTransport, wire_code_for_error};
pub use server::{
    CepServerNativeExt, HandlerContext, MethodRegistry, ServerRunConfig,
    register_production_method_registry,
};

#[cfg(test)]
mod public_api_tests {
    /// ```compile_fail
    /// use lancet_native::ToolExecutor;
    /// ```
    const _TOOL_EXECUTOR_NOT_PUBLIC: () = ();
}
