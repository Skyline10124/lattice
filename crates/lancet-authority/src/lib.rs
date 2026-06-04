//! CEP authority semantics.
//!
//! This crate owns policy evaluation, capability matching, Guard L1 write-scope
//! checks, turn-scoped active envelopes, and effect-region checking. It does not
//! own native handler I/O or server lifecycle wiring.

pub mod boundary_credit;
pub mod effect;
pub mod envelope_store;
pub mod guard;
pub mod policy;

pub use boundary_credit::{
    allowed_primitives_for, validate_credit_attribution, BoundaryCreditError,
    EffectiveBoundaryCredit, FilesystemBoundaryEvidence, LandlockApplied, MinimalMountViewApplied,
    MountNamespaceApplied, NetnsApplied, NetworkCreditRequiresNetnsEvidence, OverlayfsApplied,
};
pub use envelope_store::ActiveEnvelopeStore;
pub use policy::{
    command::CommandPolicy, BudgetLimits, CepPolicyConfig, CepTurnContext, NetworkPolicyConfig,
    ObservationPolicyConfig,
};
