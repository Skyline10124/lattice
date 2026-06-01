//! Compatibility re-exports for CEP authority effect-region semantics.

pub mod checker {
    pub use lattice_cep_authority::effect::checker::{check_effects, check_effects_against_region};
}
pub mod ledger {
    pub use lattice_cep_authority::effect::ledger::{
        AuthorizedRegionSlot, EffectLedger, EffectSource, ObservedEffect, ObservedScope,
    };
}
pub mod region {
    pub use lattice_cep_authority::effect::region::{
        normalize_region_path, normalize_write_region_path, AuthorizedEffectRegion, EffectiveEffect,
    };
}

pub use lattice_cep_authority::effect::{
    AuthorizedEffectRegion, AuthorizedRegionSlot, EffectLedger, EffectSource, EffectiveEffect,
    ObservedEffect, ObservedScope,
};
