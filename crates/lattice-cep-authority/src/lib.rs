//! CEP authority semantics.
//!
//! This crate owns policy evaluation, capability matching, Guard L1 write-scope
//! checks, turn-scoped active envelopes, and effect-region checking. It does not
//! own native handler I/O or server lifecycle wiring.

pub mod effect;
pub mod envelope_store;
pub mod guard;
pub mod policy;

pub use envelope_store::ActiveEnvelopeStore;
pub use policy::{
    command::CommandPolicy, BudgetLimits, CepPolicyConfig, CepTurnContext, NetworkPolicyConfig,
    ObservationPolicyConfig,
};
