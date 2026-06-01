//! CEP method trait and sealed catalog membership.

use lancet_protocol::{EffectDecl, ImpactKind};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::Serialize;

pub use lancet_protocol::{CapabilityPatterns, CapabilityResolveError, RequiredCapability};

pub(crate) mod sealed {
    pub trait Sealed {}
}

/// Typed CEP method contract.
pub trait CepMethod: 'static {
    const NAME: &'static str;
    const DESCRIPTION: &'static str;
    type Params: Serialize + DeserializeOwned + JsonSchema;
    type Result: Serialize + DeserializeOwned + JsonSchema;
    const IMPACT: ImpactKind;
    const CAPABILITY: RequiredCapability;
    const EFFECT: EffectDecl;
}

/// Sealed catalog membership — only types in `crate::methods` may implement this.
pub trait CatalogMethod: CepMethod + sealed::Sealed {}

/// Static descriptor for catalog projection and registry metadata.
#[derive(Clone, Debug)]
pub struct MethodDescriptor {
    pub name: &'static str,
    pub description: &'static str,
    pub impact: ImpactKind,
    pub capability: RequiredCapability,
    pub effect: EffectDecl,
    pub params_schema: fn() -> serde_json::Value,
}

impl MethodDescriptor {
    /// Build a descriptor from a catalog method type.
    pub fn for_method<M: CatalogMethod>() -> Self {
        Self {
            name: M::NAME,
            description: M::DESCRIPTION,
            impact: M::IMPACT,
            capability: M::CAPABILITY,
            effect: M::EFFECT,
            params_schema: params_schema::<M>,
        }
    }
}

fn params_schema<M: CepMethod>() -> serde_json::Value {
    serde_json::to_value(schemars::schema_for!(M::Params))
        .expect("schemars schema serializes to JSON")
}
