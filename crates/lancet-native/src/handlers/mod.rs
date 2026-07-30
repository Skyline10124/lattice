// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP/22 native CEP method handlers.

pub mod external;
pub mod mutation;
pub mod observation;
#[cfg(feature = "inspect-scaffold")]
pub mod scaffold;

pub use external::register_external_methods;
pub use mutation::register_mutation_methods;
pub use observation::register_workspace_observation_methods;
#[cfg(feature = "inspect-scaffold")]
pub use scaffold::register_inspect_scaffold_methods;
