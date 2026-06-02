//! Compatibility re-exports for CEP authority effect-region semantics.

pub mod checker {
    pub use lancet_authority::effect::checker::{
        check_denied_effect_kinds, check_effects, check_effects_against_region,
        check_effects_against_region_with_denies,
    };
}
pub mod ledger {
    pub use lancet_authority::effect::ledger::{
        AuthorizedRegionSlot, EffectLedger, EffectSource, ObservedEffect, ObservedLineChange,
        ObservedLineChangeKind, ObservedScope,
    };
}
pub mod region {
    pub use lancet_authority::effect::region::{
        normalize_region_path, normalize_write_region_path, AuthorizedEffectRegion, EffectiveEffect,
    };
}

pub use lancet_authority::effect::{
    AuthorizedEffectRegion, AuthorizedRegionSlot, EffectLedger, EffectSource, EffectiveEffect,
    ObservedEffect, ObservedLineChange, ObservedLineChangeKind, ObservedScope,
};
