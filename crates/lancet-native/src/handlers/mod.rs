// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! CEP/22 native CEP method handlers.

pub mod audit;
pub mod command;
pub mod envelope;
pub mod external;
pub mod mutation;
pub mod observation;
#[cfg(feature = "inspect-scaffold")]
pub mod scaffold;
pub mod session_state;

pub use audit::register_audit_methods;
pub use command::register_command_methods;
pub use envelope::register_envelope_methods;
pub use external::register_external_methods;
pub use mutation::register_mutation_methods;
pub use observation::register_workspace_observation_methods;
#[cfg(feature = "inspect-scaffold")]
pub use scaffold::register_inspect_scaffold_methods;
pub use session_state::register_session_state_methods;
