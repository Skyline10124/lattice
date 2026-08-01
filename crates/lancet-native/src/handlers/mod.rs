// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP/22 native CEP method handlers.

// `observation` is gone: #30 migrated `observe.workspace` to an embedded
// Component Tool whose host authority lives in
// `lancet_runtime::host_workspace`. There is no native observation handler and no
// fallback — a runtime whose composition does not embed the Component has no
// other way to serve the method.
//
// `mutation` is gone the same way for #31: `mutate.create` / `mutate.edit` are
// Component Tools (`tools/mutate-create`, `tools/mutate-edit`) over the
// `workspace-mutate` host imports.
pub mod external;
#[cfg(feature = "inspect-scaffold")]
pub mod scaffold;

pub use external::register_external_methods;
#[cfg(feature = "inspect-scaffold")]
pub use scaffold::register_inspect_scaffold_methods;
