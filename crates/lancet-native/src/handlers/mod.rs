// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP/22 native CEP method handlers.

// `observation` is gone: #30 migrated `observe.workspace` to an embedded
// Component Tool whose host authority lives in
// `lancet_runtime::host_workspace`. There is no native observation handler and no
// fallback — a runtime whose composition does not embed the Component has no
// other way to serve the method.
pub mod external;
pub mod mutation;
#[cfg(feature = "inspect-scaffold")]
pub mod scaffold;

pub use external::register_external_methods;
pub use mutation::register_mutation_methods;
#[cfg(feature = "inspect-scaffold")]
pub use scaffold::register_inspect_scaffold_methods;
